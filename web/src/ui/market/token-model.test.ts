import { describe, expect, it } from 'vitest';
import type { IndexerTokenView } from '../../data/indexer-types';
import { normalizeTokenView } from '../../data/indexer';
import { parseRegistry, displayName, type TokenInfo, type TokenRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { officialGenesis } from '../../testing/chain-fixtures';
import { needsOpenCaution, tokenTitle } from './TokenBadges';
import { buildTokenRows, filterTokens, hasIssuerControl, indexerDowngrade, labelState, parseCovenantId, templateDetail, tickerOrId, tokenBadges, tokenLabel, unknownRow } from './token-model';

const kob = await loadKobNode();
const tpl = kob.templates().find((t) => t.name === 'KCC20Ref_8x8')!;

const tplJson = (review: 'reviewed' | 'pending-review', hash = tpl.hash) => ({
  id: 'kcc20-ref-8x8', family: 'kcc20', template_hash: hash, prefix_len: tpl.prefixLen, suffix_len: tpl.suffixLen, state_len: 112, max_token_inputs: 8, max_token_outputs: 8,
  escrow: { owner_scheme: 4, borrow_scheme: 0 }, review_status: review, source: { path: 'x' },
});
const tokJson = (ticker: string, id: string, status = 'listed', verified = true) => ({
  ticker, name: `${ticker} coin`, family: 'kcc20', covenant_id: id, template_id: 'kcc20-ref-8x8', extension_commitment: 'ee'.repeat(32), extension_class: 'fixed-supply-standard',
  // lot_size: the registry schema field kob_protocol::registry still requires for a listed token; v3 orders never read it
  decimals: 8, lot_size: 100_000_000, tick: 100_000, status, verified,
});
const ID = (c: string) => c.repeat(64);

function registry(review: 'reviewed' | 'pending-review', tokens: Record<string, unknown>[], hash = tpl.hash): TokenRegistry {
  return parseRegistry({ schema_version: 1, network: 'testnet-10', templates: [tplJson(review, hash)], tokens }, { kob, network: 'testnet-10' });
}

const view = (ticker: string, id: string, asks = 0, bids = 0, over: Partial<IndexerTokenView> = {}): IndexerTokenView => ({
  ticker, covenant_id: id, template_hash: tpl.hash, extension_commitment: 'ee'.repeat(32), scale: 100_000_000, decimals: 8, open_asks: asks, open_bids: bids, ...over,
});

describe('tokenBadges', () => {
  it('verified, tradable token: a single verified badge', () => {
    const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
    expect(reg.tokens[0].tradable).toBe(true);
    expect(tokenBadges(reg.tokens[0]).map((b) => b.kind)).toEqual(['verified']);
  });

  it('a reviewed template that differs from the one pinned in this build makes a listed token untradable with the reason', () => {
    const reg = registry('reviewed', [tokJson('NEW', ID('b'))], 'ab'.repeat(32));
    const b = tokenBadges(reg.tokens[0]);
    expect(b.map((x) => x.kind)).toEqual(['verified', 'untradable']);
    expect(b[1].reason).toBe('template-mismatch');
  });

  it('pending-review and unverified tokens are tradable and show pending review plus unverified; delisted shows the status only (the reason is already told)', () => {
    const reg = registry('reviewed', [tokJson('UNV', ID('c'), 'pending-review', false), tokJson('DEL', ID('d'), 'delisted', true), tokJson('PEN', ID('e'), 'pending-review', false)]);
    expect(tokenBadges(reg.tokens[0]).map((x) => x.kind)).toEqual(['pending-review', 'unverified']);
    expect(tokenBadges(reg.tokens[1]).map((x) => x.kind)).toEqual(['delisted']);
    expect(tokenBadges(reg.tokens[2]).map((x) => x.kind)).toEqual(['pending-review', 'unverified']);
    // a pending entry that is verified against chain carries the pending badge only
    const ver = registry('reviewed', [tokJson('VER', ID('f'), 'pending-review', true)]);
    expect(tokenBadges(ver.tokens[0]).map((x) => x.kind)).toEqual(['pending-review']);
  });

});

describe('labels', () => {
  it('matches the registry displayName in English and keeps the covenant id fragment', () => {
    const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
    const t = reg.tokens[0];
    expect(tokenLabel(t, labelState(t))).toBe(displayName(t));
    expect(tokenLabel(t, 'ok')).toBe('GOOD (aaaa…aaaa) [ok]');
  });

  it('a pending-review entry says so in the label, in English (displayName) and by state word', () => {
    const reg = registry('reviewed', [tokJson('PEN', ID('e'), 'pending-review', false), tokJson('VER', ID('f'), 'pending-review', true)]);
    const [pen, ver] = reg.tokens;
    expect(labelState(pen)).toBe('unverified-pending');
    expect(labelState(ver)).toBe('verified-pending');
    expect(displayName(pen)).toBe('PEN (eeee…eeee) [unverified, pending review]');
    expect(displayName(ver)).toBe('VER (ffff…ffff) [verified, pending review]');
    expect(tokenLabel(pen, 'unverified, pending review')).toBe(displayName(pen));
    expect(labelState({ status: 'pending-review', verified: true, official: true }, 'unverified')).toBe('unverified-pending');
    expect(labelState({ status: 'pending-review', verified: true }, 'unverified')).toBe('verified-pending'); // the executor's word adds nothing to a non-official claim
    expect(labelState({ status: 'delisted', verified: false })).toBe('delisted');
  });

  it('delisted overrides verified', () => {
    expect(labelState({ status: 'delisted', verified: true })).toBe('delisted');
    expect(labelState({ status: 'listed', verified: false })).toBe('unverified');
  });
});

describe('open token list: a registry entry is never stricter than an indexer-only token', () => {
  it('a pending-review, unverified entry on a reviewed template is tradable and carries the same caution as the open-list token', () => {
    const reg = registry('reviewed', [tokJson('PEN', ID('e'), 'pending-review', false)]);
    const [row] = buildTokenRows(reg, [view('PEN', ID('e'), 1, 1)]);
    expect(row).toMatchObject({ source: 'registry', tradable: true, reason: null, labelState: 'unverified-pending', genesis: 'unverified' });
    expect(row.badges.map((b) => b.kind)).toEqual(['pending-review', 'unverified']);
    expect(row.issuerControl).toBe(false);
    expect(needsOpenCaution(row.info!)).toBe(true); // the ticket and the pre-sign screen show the unverified caution
    // the same program as an open-list token (no entry): unverified badge, tradable through the open path
    const unknown = unknownRow(reg, view('PEN', ID('9'), 1, 1));
    expect(unknown.badges.map((b) => b.kind)).toEqual(['unverified']);
    for (const b of unknown.badges) expect(row.badges.map((x) => x.kind)).toContain(b.kind);
    expect(row.powers).toEqual(unknown.powers);
    // delisted stays untradable; a template that is not reviewed still blocks
    const del = buildTokenRows(registry('reviewed', [tokJson('DEL', ID('d'), 'delisted', false)]), null)[0];
    expect(del).toMatchObject({ tradable: false, reason: 'delisted' });
    const pend = buildTokenRows(registry('pending-review', [tokJson('PEN', ID('e'), 'pending-review', false)]), null)[0];
    expect(pend).toMatchObject({ tradable: false, reason: 'template-pending-review' });
    expect(pend.badges.map((b) => b.kind)).toEqual(['pending-review', 'unverified', 'untradable']);
  });
});

describe('buildTokenRows', () => {
  const reg = registry('reviewed', [tokJson('KRON', ID('a')), tokJson('GOOD', ID('b'))]);

  it('lists registry tokens first with their indexer counts, then unknown ones as unverified and not tradable', () => {
    const rows = buildTokenRows(reg, [view('GOOD', ID('b'), 3, 4), view('WIDE', ID('9'), 1, 1), view('QUIET', ID('8'), 0, 0)]);
    expect(rows.map((r) => r.ticker)).toEqual(['KRON', 'GOOD', 'WIDE', 'QUIET']);
    expect(rows[1]).toMatchObject({ source: 'registry', openAsks: 3, openBids: 4, tradable: true });
    expect(rows[0].openAsks).toBeNull(); // the indexer has not seen it
    expect(rows[2]).toMatchObject({ source: 'indexer', tradable: false, reason: 'unverified', labelState: 'unverified' });
    expect(rows[2].badges.map((b) => b.kind)).toEqual(['unverified']);
  });

  it('cross-checks the indexer view against the registry', () => {
    const rows = buildTokenRows(reg, [view('GOOD', ID('b'), 0, 0, { template_hash: 'cd'.repeat(32) })]);
    expect(rows[1].indexerProblems).toContain('template-hash-mismatch');
  });

  it('flags a lookalike of a registered ticker as a strong warning and sanitizes hostile text', () => {
    const fake = unknownRow(reg, view('KR0N', ID('7')));
    expect(fake.lookalike?.level).toBe('strong');
    expect(fake.lookalike?.lookalikes[0].token.ticker).toBe('KRON');
    expect(fake.tradable).toBe(false);
    const evil = unknownRow(reg, view('A‮B\u0000C', ID('6')));
    expect(evil.ticker).not.toMatch(/[‮\u0000]/);
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

describe('registry standing, powers and empty tickers (GET /v1/tokens)', () => {
  const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);

  it('an official standing shows the official badge on a registry token; without a standing the registry decides', () => {
    const [plain] = buildTokenRows(reg, [view('GOOD', ID('a'))]);
    expect(plain.badges.map((b) => b.kind)).toEqual(['verified']);
    // `official` comes from the registry flag alone: the indexer's word does not upgrade a merely verified token
    const [off] = buildTokenRows(reg, [view('GOOD', ID('a'), 0, 0, { standing: 'official' })]);
    expect(off.badges.map((b) => b.kind)).toEqual(['verified']);
    expect(off.labelState).toBe('verified');
    const officialReg = registry('reviewed', [{ ...tokJson('GOOD', ID('a')), official: true, ...officialGenesis() }]);
    const [real] = buildTokenRows(officialReg, [view('GOOD', ID('a'))]);
    expect(real.badges.map((b) => b.kind)).toEqual(['official']);
    expect(tokenLabel(real, 'official')).toBe('GOOD (aaaa…aaaa) [official]');
    // ...and an indexer may still downgrade it
    expect(buildTokenRows(officialReg, [view('GOOD', ID('a'), 0, 0, { standing: 'unverified' })])[0].labelState).toBe('unverified');
  });

  it('a delisted standing beats everything; an unverified standing adds nothing to a verified, non-official registry token (one word for it on every page)', () => {
    expect(buildTokenRows(reg, [view('GOOD', ID('a'), 0, 0, { standing: 'delisted' })])[0].badges.map((b) => b.kind)).toEqual(['delisted']);
    // the executor says `unverified` of every token that is not official: that is not "less than verified" (soak registry: verified tokens, none official)
    const [row] = buildTokenRows(reg, [view('GOOD', ID('a'), 0, 0, { standing: 'unverified' })]);
    expect(row.labelState).toBe('verified');
    expect(row.badges.map((b) => b.kind)).toEqual(['verified']);
    expect(row.standing).toBeNull();
  });

  it('the market row and the registry-only label (My orders, balances) agree for every standing the indexer can send, except an official claim the executor does not confirm', () => {
    const t = reg.tokens[0];
    for (const standing of [undefined, 'official', 'unverified'] as const) {
      const [row] = buildTokenRows(reg, [view('GOOD', ID('a'), 0, 0, standing ? { standing } : {})]);
      expect(row.labelState, String(standing)).toBe(labelState(t));
      expect(row.badges.map((b) => b.kind), String(standing)).toEqual(tokenBadges(t).map((b) => b.kind));
      expect(tokenTitle(row), String(standing)).toBe(`GOOD (aaaa…aaaa) [verified]`);
    }
    // a token the registry itself marks unverified stays unverified on both
    const bad = registry('reviewed', [tokJson('GOOD', ID('a'), 'pending-review', false)]);
    expect(buildTokenRows(bad, [view('GOOD', ID('a'), 0, 0, { standing: 'unverified' })])[0].labelState).toBe('unverified-pending');
    expect(labelState(bad.tokens[0])).toBe('unverified-pending');
  });

  it('indexerDowngrade: delisted always; unverified only against an official registry claim; official never upgrades', () => {
    expect(indexerDowngrade({}, 'delisted')).toBe('delisted');
    expect(indexerDowngrade({ official: true }, 'delisted')).toBe('delisted');
    expect(indexerDowngrade({ official: true }, 'unverified')).toBe('unverified');
    expect(indexerDowngrade({ official: false }, 'unverified')).toBeNull();
    expect(indexerDowngrade({}, 'unverified')).toBeNull();
    expect(indexerDowngrade({ official: true }, 'official')).toBeNull();
    expect(indexerDowngrade({}, 'official')).toBeNull();
    expect(indexerDowngrade({ official: true }, null)).toBeNull();
  });

  it('a token outside the registry is unverified by default, delisted only when the executor says so, never official (registry only); never tradable here', () => {
    const rows = buildTokenRows(reg, [
      view('AAA', ID('1')),
      view('BBB', ID('2'), 0, 0, { standing: 'official' }),
      view('CCC', ID('3'), 0, 0, { standing: 'delisted' }),
      view('DDD', ID('4'), 0, 0, { standing: 'unverified' }),
    ]).slice(1);
    const by = Object.fromEntries(rows.map((r) => [r.ticker, r]));
    expect(by.AAA.badges.map((b) => b.kind)).toEqual(['unverified']);
    expect(by.BBB.badges.map((b) => b.kind)).toEqual(['unverified']);
    expect(by.BBB.labelState).toBe('unverified');
    expect(by.BBB.indexerProblems).toEqual(['standing-official-unregistered']);
    expect(tokenLabel(by.BBB, 'unverified')).not.toContain('official');
    expect(by.CCC.badges.map((b) => b.kind)).toEqual(['delisted']);
    expect(by.DDD.badges.map((b) => b.kind)).toEqual(['unverified']);
    expect(rows.every((r) => !r.tradable)).toBe(true);
  });

  it('an empty ticker shows the short covenant id instead, everywhere a label is built, and is searchable by it', () => {
    const rows = buildTokenRows(reg, [view('', ID('e'))]);
    const r = rows[1];
    expect(r.ticker).toBe('');
    expect(tokenLabel(r, 'unverified')).toBe('eeeeeeee…eeeeeeee [unverified]');
    expect(tickerOrId(r.ticker, r.covenantId)).toBe('eeeeeeee…eeeeeeee');
    expect(tickerOrId('GOOD', ID('a'))).toBe('GOOD');
    expect(filterTokens(rows, 'eeee').map((x) => x.covenantId)).toEqual([ID('e')]);
  });

  it('two tokens with the same ticker stay two rows, told apart by covenant id and template hash', () => {
    const rows = buildTokenRows(reg, [view('SAME', ID('5')), view('SAME', ID('6'), 0, 0, { template_hash: 'ff'.repeat(32) })]).slice(1);
    expect(rows.map((r) => tokenLabel(r, 'x'))).toEqual(['SAME (55555555…55555555) [x]', 'SAME (66666666…66666666) [x]']);
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

  it('tokenBadges takes the standing as a second argument', () => {
    const t = reg.tokens[0];
    expect(tokenBadges(t, 'official').map((b) => b.kind)).toEqual(['verified']);
    expect(tokenBadges(t, 'unverified').map((b) => b.kind)).toEqual(['verified']);
    expect(tokenBadges({ ...t, official: true }, 'unverified').map((b) => b.kind)).toEqual(['unverified']);
    expect(tokenBadges(t, 'delisted').map((b) => b.kind)).toEqual(['delisted']);
    expect(tokenBadges({ ...t, official: true }, 'official').map((b) => b.kind)).toEqual(['official']);
    expect(tokenBadges(t, null).map((b) => b.kind)).toEqual(['verified']);
  });
});

describe('the registry always wins over the indexer for trust and issuer powers', () => {
  const reg = registry('reviewed', [tokJson('GOOD', ID('a'))]);
  const REAL = 'aa'.repeat(32);
  const withCaps = (caps: string[], official = false) => {
    const r = registry('reviewed', [{ ...tokJson('REAL', REAL), official }]);
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

  it('an unregistered token is never [official], whatever ticker and standing the indexer chooses', () => {
    const scam = normalizeTokenView(view('KASPER', 'ee'.repeat(32), 0, 0, { standing: 'official' }));
    const row = buildTokenRows(reg, [scam]).find((r) => r.covenantId === scam.covenant_id)!;
    expect(row.info).toBeNull();
    expect(row.badges.map((b) => b.kind)).toEqual(['unverified']);
    expect(row.labelState).toBe('unverified');
    expect(tokenLabel(row, row.labelState)).toBe('KASPER (eeeeeeee…eeeeeeee) [unverified]');
    expect(row.tradable).toBe(false);
  });

  it('genesis verification comes from the registry statement only: verified when it says so, "not verified" when it does not, nothing for unregistered tokens', () => {
    const r0 = buildTokenRows(withCaps([]), null)[0];
    expect(r0.genesis).toBe('unverified'); // registry silent = not verified
    const r1 = registry('reviewed', [{ ...tokJson('REAL', REAL), genesis_verified: true, warning: 'audited by hand' }]);
    expect(r1.tokens[0]).toMatchObject({ genesisVerified: true, warning: 'audited by hand' });
    expect(buildTokenRows(r1, null)[0].genesis).toBe('verified');
    expect(buildTokenRows(r1, [view('X', ID('7'))])[1].genesis).toBeNull();
    // an indexer field of the same name is not read
    expect(buildTokenRows(withCaps([]), [view('REAL', REAL, 0, 0, { genesis_verified: true } as never)])[0].genesis).toBe('unverified');
  });
});
