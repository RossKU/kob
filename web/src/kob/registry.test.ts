import { describe, expect, it } from 'vitest';
import {
  RegistryError, displayName, hasBadChar, lookalikeReport, normalizeTicker, parseRegistry, sanitizeUntrusted, tokenById, tokenMarketInput, tradableTokens,
  verifyIndexerToken, verifyTokenUtxo, type ChainTokenUtxo, type RegistryIssueCode, type TokenRegistry,
} from './registry';
import { exampleRegistryJson, officialGenesis, placeGolden, shippedRegistryJson, TOKEN, tradableRegistryJson } from '../testing/chain-fixtures';
import { loadKobNode } from './wasm.node';
import { synthesizeOpenToken } from './open-token';
import type { IndexerTokenView } from '../data/indexer-types';

const kob = loadKobNode();
const parse = (doc: unknown, network?: string): TokenRegistry => parseRegistry(doc, { kob, network });
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const codes = (doc: unknown, network?: string): RegistryIssueCode[] => {
  try {
    parse(doc, network);
  } catch (e) {
    if (e instanceof RegistryError) return e.issues.map((i) => i.code);
    throw e;
  }
  return [];
};

/** A genesis record that found no live mint authority (what official needs besides genesis_verified). */
const GENESIS = officialGenesis().genesis;

describe('shipped registries', () => {
  it('the shipped mainnet registry parses: the census KRON tokens listed after the listing verification, official except the two test tokens, five programs (four pinned and matching this wasm build)', () => {
    const r = parse(shippedRegistryJson(), 'mainnet');
    expect(r.network).toBe('mainnet');
    expect(r.tokens.length).toBe(8);
    // listing verification (registry/evidence/mainnet-genesis.json, `kob registry verify-genesis`): genesis verified (C1), no live minter (C2)
    expect(r.tokens.every((t) => t.family === 'kron' && t.status === 'listed' && t.verified && t.genesisVerified === true)).toBe(true);
    // founder 2026-10-03: PEPE ("The Ultimate test") and DNBT ("dont buy this is test") are test tokens per their own names: verified, listed, NOT official,
    // with a maintainer warning (PEPE: also not the well-known PEPE); the other six are official without a warning
    expect(r.tokens.filter((t) => !t.official).map((t) => t.ticker)).toEqual(['PEPE', 'DNBT']);
    for (const t of r.tokens) {
      if (t.official) expect(t.warning, t.ticker).toBeNull();
      else expect(t.warning, t.ticker).toMatch(/^Not official: a test token per its own name/);
    }
    const pepe = r.tokens.find((t) => t.ticker === 'PEPE')!;
    expect(pepe.warning).toContain('well-known PEPE');
    expect(displayName(pepe)).toBe('PEPE (a73c…47cd) [verified]');
    // a namesake of a disowned test token is a ticker collision, not an impersonation; a namesake of an official token stays a strong warning
    const other = 'cd'.repeat(32);
    expect(lookalikeReport(r, 'PEPE', other)).toMatchObject({ level: 'shared', lookalikes: [{ kind: 'same-ticker' }] });
    expect(lookalikeReport(r, 'PEPE', other).message).toContain('marks as not official');
    expect(lookalikeReport(r, 'DNBT', other).level).toBe('shared');
    expect(lookalikeReport(r, 'KR0N', other).level).toBe('strong');
    expect(lookalikeReport(r, 'PEPE', pepe.covenantId).level).toBe('none');
    const raw = JSON.parse(shippedRegistryJson());
    expect(raw.tokens.every((t: { genesis?: { live_minters?: string[]; minter_outputs: number[] } }) => t.genesis?.live_minters?.length === 0 && t.genesis.minter_outputs.length === 0)).toBe(true);
    // the KRON programs are reviewed with conditions (internal review, `crates/kob-tests/tests/review_b2_kron.rs`)
    expect(r.tokens.every((t) => t.tradable && t.untradableReason === null && t.templateMatchesPinned)).toBe(true);
    const review = Object.fromEntries(r.templates.map((t) => [t.id, t.review_status]));
    expect(review).toMatchObject({ 'kcc20-ref-3x3': 'pending-review', 'kcc20-ref-8x8': 'pending-review', 'kron-2433': 'reviewed', 'kron-2732': 'reviewed', 'kcc20-kaspacom-0-2-5': 'pending-review' });
    for (const id of ['kron-2433', 'kron-2732']) {
      expect(r.templates.find((t) => t.id === id)!.risks!.some((x) => x.startsWith('Reviewed with conditions') && x.includes('live is_minter'))).toBe(true);
    }
    expect(r.templates.map((t) => t.id).slice(0, 4)).toEqual(['kcc20-ref-3x3', 'kcc20-ref-8x8', 'kron-2433', 'kron-2732']);
    // a third-party program lists what its authorities can do: it reaches the token info (no freeze / seize here)
    const kc = r.templates.find((t) => t.id.startsWith('kcc20-kaspacom'));
    if (kc) expect(kc.capabilities).toEqual(['mint-authority', 'public-mint', 'burn']);
    const byId = Object.fromEntries(r.templateChecks.map((c) => [c.id, c]));
    expect(byId['kcc20-ref-3x3']).toMatchObject({ program: 'KCC20Ref', matchesPinned: true, problems: [] });
    expect(byId['kcc20-ref-8x8']).toMatchObject({ program: 'KCC20Ref_8x8', matchesPinned: true });
    expect(byId['kron-2433']).toMatchObject({ program: 'KronToken2433', matchesPinned: true, problems: [] });
    expect(byId['kron-2732']).toMatchObject({ program: 'KronToken2732', matchesPinned: true, problems: [] });
    // the published public-mint build of the reference: pinned as its actor-type handle (the context push in a 34-byte prefix)
    expect(byId['kcc20-ref-public-mint']).toMatchObject({ program: 'KCC20PublicMint', matchesPinned: true, problems: [] });
    const pm = r.templates.find((t) => t.id === 'kcc20-ref-public-mint')!;
    expect([pm.prefix_len, pm.state_len, pm.suffix_len, pm.max_token_inputs, pm.review_status]).toEqual([34, 112, 3885, 3, 'pending-review']);
  });

  it('the example TN10 registry parses; its pending, unverified tokens are tradable (their templates are reviewed and pinned)', () => {
    const r = parse(exampleRegistryJson(), 'testnet-10');
    expect(r.tokens.map((t) => t.ticker)).toEqual(['EXKCC', 'EXKRON']);
    expect(r.tokens.every((t) => t.status === 'pending-review' && !t.verified)).toBe(true);
    expect(r.tokens.map((t) => t.untradableReason)).toEqual([null, null]);
    expect(tradableTokens(r).map((t) => t.ticker)).toEqual(['EXKCC', 'EXKRON']);
  });

  it('refuses a registry of another network', () => {
    expect(codes(JSON.parse(shippedRegistryJson()), 'testnet-10')).toEqual(['wrong-network']);
    expect(codes(JSON.parse(exampleRegistryJson()), 'mainnet')).toEqual(['wrong-network']);
  });

  it('refuses malformed JSON and non-objects', () => {
    expect(codes('{nope')).toEqual(['json']);
    expect(codes([])).toEqual(['wrong-type']);
    expect(codes(null)).toEqual(['wrong-type']);
  });
});

describe('tradability (listing verified against the pinned templates)', () => {
  it('a listed, verified token on a reviewed, pinned program is tradable and maps to its kob-wasm program', () => {
    const r = parse(tradableRegistryJson(), 'testnet-10');
    const tst = tokenById(r, TOKEN.covenantId)!;
    expect(tst).toMatchObject({ tradable: true, untradableReason: null, program: 'KCC20Ref', tick: null, decimals: 3, slots: { inputs: 3, outputs: 3 } });
    expect(tst.templateHash).toBe(TOKEN.templateHash);
    expect(tradableTokens(r).map((t) => t.program)).toEqual(['KCC20Ref', 'KCC20Ref_8x8']);
    expect(tokenById(r, '80'.repeat(32))!.slots).toEqual({ inputs: 8, outputs: 8 });
  });

  it('a reviewed-less template makes listed tokens impossible (validation) and its pending-review tokens viewable but not tradable (the template gates, not the token)', () => {
    const doc = tradableRegistryJson();
    doc.templates[0].review_status = 'pending-review';
    expect(codes(doc)).toContain('not-listable');
    doc.tokens[0].status = 'pending-review';
    doc.tokens[0].verified = true;
    const r = parse(doc);
    expect(tokenById(r, TOKEN.covenantId)).toMatchObject({ tradable: false, untradableReason: 'template-pending-review' });
  });

  it('a pending-review, unverified entry on a reviewed template is tradable (open token list); only delisted and template problems block', () => {
    const doc = tradableRegistryJson();
    doc.tokens[0].status = 'pending-review';
    doc.tokens[0].verified = false;
    doc.tokens[1].status = 'delisted';
    const r = parse(doc);
    expect(r.tokens.map((t) => t.untradableReason)).toEqual([null, 'delisted']);
    expect(r.tokens.map((t) => t.tradable)).toEqual([true, false]);
    expect(tokenById(r, TOKEN.covenantId)).toMatchObject({ status: 'pending-review', verified: false, official: false, program: 'KCC20Ref', tick: null });
    // listed but unverified (an entry the maintainers have not checked against chain yet) is no stricter
    const doc2 = tradableRegistryJson();
    doc2.tokens[0].verified = false;
    expect(codes(doc2)).toContain('not-listable'); // a LISTED status still requires verification: the schema rule is unchanged
    doc2.tokens[0].status = 'pending-review';
    expect(tokenById(parse(doc2), TOKEN.covenantId)!.tradable).toBe(true);
    // the template checks still block
    const doc3 = tradableRegistryJson();
    doc3.tokens[0].status = 'pending-review';
    doc3.tokens[0].verified = false;
    doc3.templates[0].template_hash = 'ab'.repeat(32);
    expect(tokenById(parse(doc3), TOKEN.covenantId)).toMatchObject({ tradable: false, untradableReason: 'template-mismatch' });
    // protocol v3 has no lots and no price tick: an entry without the registry's legacy lot size or tick is tradable and LISTABLE (the scale
    // comes from its decimals); a legacy value is read and ignored (registry.rs), never validated
    const doc4 = tradableRegistryJson();
    doc4.tokens[0].lot_size = null;
    doc4.tokens[0].tick = null;
    expect(tokenById(parse(doc4), TOKEN.covenantId)).toMatchObject({ status: 'listed', tradable: true, untradableReason: null, tick: null });
    const doc5 = tradableRegistryJson();
    delete doc5.tokens[0].lot_size;
    delete doc5.tokens[0].tick;
    expect(tokenById(parse(doc5), TOKEN.covenantId)).toMatchObject({ status: 'listed', tradable: true, tick: null });
    const doc6 = tradableRegistryJson();
    doc6.tokens[0].lot_size = 0;
    doc6.tokens[0].tick = -5;
    expect(tokenById(parse(doc6), TOKEN.covenantId)).toMatchObject({ tradable: true, tick: null });
  });

  it('a pending-review, unverified registry entry is treated exactly like the open-list token of the same program (tradable, unverified, not official)', () => {
    const doc = tradableRegistryJson();
    doc.tokens[0].status = 'pending-review';
    doc.tokens[0].verified = false;
    const entry = tokenById(parse(doc), TOKEN.covenantId)!;
    const open = synthesizeOpenToken(kob, { covenant_id: entry.covenantId, template_hash: entry.templateHash, extension_commitment: entry.extensionCommitment } as any, [{ scale: 1000 }], null, null);
    expect(open.ok).toBe(true);
    if (!open.ok) return;
    for (const k of ['tradable', 'untradableReason', 'verified', 'official', 'program', 'templateMatchesPinned', 'family', 'capabilities'] as const) expect(entry[k]).toEqual(open.info[k]);
  });

  it.each([
    ['template hash', (t: any) => { t.template_hash = 'ab'.repeat(32); }, 'hash-mismatch'],
    ['prefix length', (t: any) => { t.prefix_len = 2; }, 'prefix-mismatch'],
    ['suffix length', (t: any) => { t.suffix_len = 2978; }, 'suffix-mismatch'],
    ['slot limits', (t: any) => { t.max_token_inputs = 4; t.max_token_outputs = 4; }, 'slots-mismatch'],
  ])('a tampered %s makes the token untradable (template-mismatch), never trusted', (_n, mutate, problem) => {
    const doc = tradableRegistryJson();
    mutate(doc.templates.find((t: any) => t.id === 'kcc20-ref-3x3'));
    const r = parse(doc);
    const check = r.templateChecks.find((c) => c.id === 'kcc20-ref-3x3')!;
    expect(check.matchesPinned).toBe(false);
    expect(check.problems).toContain(problem);
    expect(tokenById(r, TOKEN.covenantId)).toMatchObject({ tradable: false, untradableReason: 'template-mismatch' });
    // the other program is unaffected
    expect(tokenById(r, '80'.repeat(32))!.tradable).toBe(true);
  });

  it('a template id that points at another program\'s hash is a mismatch', () => {
    const doc = tradableRegistryJson();
    const a = doc.templates.find((t: any) => t.id === 'kcc20-ref-3x3');
    const b = doc.templates.find((t: any) => t.id === 'kcc20-ref-8x8');
    [a.template_hash, b.template_hash] = [b.template_hash, a.template_hash];
    const r = parse(doc);
    expect(r.templateChecks.filter((c) => c.problems.includes('hash-mismatch')).map((c) => c.id)).toEqual(['kcc20-ref-3x3', 'kcc20-ref-8x8']);
    expect(tradableTokens(r)).toEqual([]);
  });

  it('kron templates are supported: a listed, verified token on a reviewed kron template is tradable', () => {
    const doc = tradableRegistryJson();
    for (const t of doc.templates) if (t.id === 'kron-2433') t.review_status = 'reviewed';
    doc.tokens.push({
      ticker: 'KRONX', name: 'Kron example', family: 'kron', covenant_id: '90'.repeat(32), template_id: 'kron-2433', extension_commitment: null, extension_class: 'none',
      decimals: 8, lot_size: 1000000, tick: 100, status: 'listed', verified: true,
    });
    const r = parse(doc);
    expect(tokenById(r, '90'.repeat(32))).toMatchObject({ tradable: true, untradableReason: null, program: 'KronToken2433', family: 'kron' });
  });
});

describe('validation (mirror of the Rust rules)', () => {
  const base = () => tradableRegistryJson();

  it('rejects homoglyph-confusable tickers (O->0, I/L->1, S->5, B->8, rn->m)', () => {
    const doc = base();
    doc.tokens[1].ticker = 'TST';
    doc.tokens[1].covenant_id = '81'.repeat(32);
    expect(codes(doc)).toContain('bad-ticker'); // exact duplicate
    doc.tokens[1].ticker = 'T5T'; // S and 5 are confusable too
    expect(codes(doc)).toEqual(['confusable-ticker']);
    doc.tokens[1].ticker = 'TSU';
    expect(codes(doc)).toEqual([]);
    const d2 = base();
    d2.tokens[0].ticker = 'KRON';
    d2.tokens[1].ticker = 'KR0N';
    expect(codes(d2)).toEqual(['confusable-ticker']);
    const d3 = base();
    d3.tokens[0].ticker = 'LIT';
    d3.tokens[1].ticker = '1IT';
    expect(codes(d3)).toEqual(['confusable-ticker']);
  });

  it('rejects malformed tickers', () => {
    for (const bad of ['kron', 'K', 'TOOLONGTICKER1', 'KR ON', 'KRÖN']) {
      const doc = base();
      doc.tokens[0].ticker = bad;
      expect(codes(doc), bad).toContain('bad-ticker');
    }
  });

  it('rejects control, zero-width and bidi characters in names and display text', () => {
    for (const bad of ['Good​Name', 'Evil‮name', 'Ctl\u0007', 'Bom﻿', 'iso⁦late']) {
      const doc = base();
      doc.tokens[0].name = bad;
      expect(codes(doc), JSON.stringify(bad)).toEqual(['bad-display']);
    }
    const doc = base();
    doc.tokens[0].display = { description: 'x​y' };
    expect(codes(doc)).toEqual(['bad-display']);
    doc.tokens[0].display = { website: 'http://insecure.example' };
    expect(codes(doc)).toEqual(['bad-display']);
    doc.tokens[0].display = { icon: 'ftp://x' };
    expect(codes(doc)).toEqual(['bad-display']);
    doc.tokens[0].display = { website: 'https://ok.example', icon: 'ipfs://cid', kcc23: {} };
    expect(codes(doc)).toEqual([]);
    expect(hasBadChar('plain')).toBe(false);
    expect(hasBadChar('a‍b')).toBe(true);
  });

  it('rejects unknown fields at every level', () => {
    for (const mutate of [
      (d: any) => { d.extra = 1; },
      (d: any) => { d.templates[0].extra = 1; },
      (d: any) => { d.templates[0].escrow.extra = 1; },
      (d: any) => { d.tokens[0].extra = 1; },
      (d: any) => { d.tokens[0].display = { extra: 'x' }; },
    ]) {
      const doc = base();
      mutate(doc);
      expect(codes(doc)).toEqual(['unknown-field']);
    }
  });

  it('rejects bad hex, unknown templates, family mismatches and duplicates', () => {
    const cases: [string, (d: any) => void, RegistryIssueCode][] = [
      ['covenant id length', (d) => { d.tokens[0].covenant_id = 'ab'; }, 'bad-hex'],
      ['upper-case hex', (d) => { d.tokens[0].covenant_id = 'AB'.repeat(32); }, 'bad-hex'],
      ['template hash', (d) => { d.templates[0].template_hash = 'zz'.repeat(32); }, 'bad-hex'],
      ['unknown template', (d) => { d.tokens[0].template_id = 'nope'; }, 'unknown-template'],
      ['family mismatch', (d) => { d.tokens[0].family = 'kron'; }, 'family-mismatch'],
      ['duplicate covenant id', (d) => { d.tokens[1].covenant_id = d.tokens[0].covenant_id; }, 'duplicate-covenant-id'],
      ['duplicate template id', (d) => { d.templates[1].id = d.templates[0].id; }, 'duplicate-template-id'],
      ['duplicate template hash', (d) => { d.templates[1].template_hash = d.templates[0].template_hash; }, 'duplicate-template-hash'],
      ['decimals', (d) => { d.tokens[0].decimals = 19; }, 'decimals'],
      ['schema version', (d) => { d.schema_version = 2; }, 'schema-version'],
      ['network', (d) => { d.network = 'mars'; }, 'unknown-network'],
      ['kcc20 without extension', (d) => { d.tokens[0].extension_commitment = null; }, 'extension'],
      ['none class with a commitment', (d) => { d.tokens[0].extension_class = 'none'; }, 'extension'],
      ['slot limits differ from the template', (d) => { d.tokens[0].max_token_inputs = 5; }, 'slot-limit-mismatch'],
      ['listed without verification', (d) => { d.tokens[0].verified = false; }, 'not-listable'],
      ['kcc20 escrow scheme', (d) => { d.templates[0].escrow.owner_scheme = 0; }, 'template-invalid'],
      ['unknown capability', (d) => { d.templates[0].capabilities = ['teleport']; }, 'template-invalid'],
      ['duplicate capability', (d) => { d.templates[0].capabilities = ['burn', 'burn']; }, 'template-invalid'],
      ['official without verification', (d) => { d.tokens[0].official = true; d.tokens[0].verified = false; }, 'not-listable'],
      ['official but not listed', (d) => { d.tokens[0].official = true; d.tokens[0].status = 'pending-review'; }, 'not-listable'],
      ['official is a boolean', (d) => { d.tokens[0].official = 'yes'; }, 'wrong-type'],
    ];
    for (const [name, mutate, code] of cases) {
      const doc = clone(base());
      mutate(doc);
      expect(codes(doc), name).toContain(code);
    }
  });

  it('reads template capabilities and the official flag: powers reach the token info, [official] the display name', () => {
    const doc = clone(base());
    doc.templates[0].capabilities = ['freeze', 'seize'];
    doc.tokens[0].official = true;
    doc.tokens[0].genesis_verified = true;
    doc.tokens[0].genesis = GENESIS;
    const r = parse(doc);
    const t = r.tokens[0];
    expect(t).toMatchObject({ official: true, capabilities: ['freeze', 'seize'] });
    expect(displayName(t).endsWith(' [official]')).toBe(true);
    expect(displayName({ ...t, status: 'delisted' }).endsWith(' [delisted]')).toBe(true);
    expect(r.tokens[1]).toMatchObject({ official: false, capabilities: [] });
    expect(displayName(r.tokens[1]).endsWith(' [verified]')).toBe(true);
  });

  it('rejects two tokens with the same identity', () => {
    const doc = base();
    doc.tokens[1] = { ...doc.tokens[0], ticker: 'COPY' };
    const c = codes(doc);
    expect(c).toContain('duplicate-identity');
    expect(c).toContain('duplicate-covenant-id');
  });

  it('collects every finding, not only the first', () => {
    const doc = base();
    doc.tokens[0].decimals = 99;
    doc.tokens[1].name = '';
    try {
      parse(doc);
      throw new Error('accepted');
    } catch (e) {
      expect(e).toBeInstanceOf(RegistryError);
      expect((e as RegistryError).issues.length).toBeGreaterThanOrEqual(2);
      expect((e as RegistryError).message).toContain('decimals');
    }
  });
});

describe('display and lookalikes', () => {
  const reg = () => parse(tradableRegistryJson());

  it('displayName is TICKER (abcd...1234) [state], never the name alone', () => {
    const r = reg();
    const tst = tokenById(r, TOKEN.covenantId)!;
    expect(displayName(tst)).toBe('TST (7070…7070) [verified]');
    expect(displayName({ ...tst, verified: false })).toBe('TST (7070…7070) [unverified]');
    expect(displayName({ ...tst, status: 'delisted' })).toBe('TST (7070…7070) [delisted]');
    expect(displayName(tst)).not.toContain(tst.name);
  });

  it('normalizeTicker matches the Rust homoglyph rule', () => {
    expect(normalizeTicker('KRON')).toBe('KR0N');
    expect(normalizeTicker('kr0n')).toBe('KR0N');
    expect(normalizeTicker('ILL')).toBe('111');
    expect(normalizeTicker('A1B')).toBe('A18');
    // 5/S, 8/B, rn/m, vv/w
    expect(normalizeTicker('BASS')).toBe(normalizeTicker('8A55'));
    expect(normalizeTicker('KASPERN')).toBe(normalizeTicker('KASPEM'));
    expect(normalizeTicker('vveb')).toBe(normalizeTicker('WE8'));
  });

  it('normalizeTicker folds Unicode lookalikes (NFKC, Cyrillic, Greek, Armenian, combining marks) to the same skeleton as ASCII', () => {
    const ascii = normalizeTicker('KRON');
    expect(normalizeTicker('КРОН')).toBe(normalizeTicker('KPOH')); // Cyrillic Н is an H, not an N
    expect(normalizeTicker('КРОH')).toBe(normalizeTicker('KPOH'));
    expect(normalizeTicker('АВЕ')).toBe(normalizeTicker('ABE')); // Cyrillic A B E
    expect(normalizeTicker('ΑΒΕ')).toBe(normalizeTicker('ABE')); // Greek
    expect(normalizeTicker('рере')).toBe(normalizeTicker('PEPE')); // Cyrillic lowercase р е р е
    expect(normalizeTicker('ＫＲＯＮ')).toBe(ascii); // full-width KRON
    expect(normalizeTicker('ｋｒｏｎ')).toBe(ascii); // full-width lowercase
    expect(normalizeTicker('ḰRON')).toBe(ascii); // combining acute on K
    expect(normalizeTicker('ⅠⅬ')).toBe(normalizeTicker('IL')); // roman numerals
    expect(normalizeTicker('ՕՕ')).toBe(normalizeTicker('OO')); // Armenian Oh
    expect(normalizeTicker('ß')).toBe(normalizeTicker('SS'));
  });

  it('lookalikeReport: registered id is fine, unknown is flagged, a copied ticker names the real token', () => {
    const r = reg();
    expect(lookalikeReport(r, 'TST', TOKEN.covenantId)).toMatchObject({ level: 'none', lookalikes: [] });
    expect(lookalikeReport(r, 'ZZZ', 'cd'.repeat(32))).toMatchObject({ level: 'unknown', lookalikes: [] });
    const same = lookalikeReport(r, 'TST', 'cd'.repeat(32));
    expect(same.level).toBe('strong');
    expect(same.lookalikes[0]).toMatchObject({ kind: 'same-ticker' });
    expect(same.lookalikes[0].token.covenantId).toBe(TOKEN.covenantId);
    expect(same.message).toContain('TST (7070…7070)');
    const homoglyph = lookalikeReport(r, 'E1GHT', 'cd'.repeat(32));
    expect(homoglyph.level).toBe('strong');
    expect(homoglyph.lookalikes[0]).toMatchObject({ kind: 'confusable-ticker' });
    expect(lookalikeReport(r, 'tst', 'cd'.repeat(32)).lookalikes[0].kind).toBe('same-ticker');
  });

  it('lookalikeReport: a namesake is only a ticker collision (shared) when every registered match is NOT official AND carries a maintainer warning', () => {
    const other = 'cd'.repeat(32);
    const doc = tradableRegistryJson();
    doc.tokens[0].warning = 'Not official: a test token per its own name';
    // TST: not official, warned -> shared
    const warned = parse(doc);
    expect(lookalikeReport(warned, 'TST', other)).toMatchObject({ level: 'shared', lookalikes: [{ kind: 'same-ticker' }] });
    expect(lookalikeReport(warned, 'T5T', other).level).toBe('shared');
    // not official, no warning -> still strong (the registry does not disown it)
    expect(lookalikeReport(reg(), 'TST', other).level).toBe('strong');
    // official (a warning does not make an official token a non-target) -> strong
    const off = tradableRegistryJson();
    Object.assign(off.tokens[0], { official: true, warning: 'w' }, officialGenesis());
    expect(lookalikeReport(parse(off), 'TST', other).level).toBe('strong');
    // one disowned match and one plain match -> strong (the plain one may be impersonated)
    const both = tradableRegistryJson();
    both.tokens[0].warning = 'w';
    both.tokens[1].name = 'TST';
    expect(lookalikeReport(parse(both), 'TST', other).level).toBe('strong');
  });

  it('lookalikeReport: Unicode homoglyphs, full-width letters and the 5/S, 8/B, rn/m folds are strong lookalikes too', () => {
    const r = reg();
    const other = 'cd'.repeat(32);
    // Cyrillic Т (U+0422) and Ѕ (U+0405) in "TST"
    for (const t of ['ТЅТ', 'ТSТ', 'T5T', 'ＴＳＴ', 'tśt', 'тѕт']) {
      const rep = lookalikeReport(r, t, other);
      expect(rep.level, t).toBe('strong');
      expect(rep.lookalikes[0].token.covenantId).toBe(TOKEN.covenantId);
    }
    expect(lookalikeReport(r, 'TST', TOKEN.covenantId).level).toBe('none');
    expect(lookalikeReport(r, 'ТЅТ', TOKEN.covenantId).level).toBe('none');
    expect(lookalikeReport(r, 'TSU', other).level).toBe('unknown');
  });

  it('lookalikeReport: a token NAME that copies a registered ticker or name is flagged (case, spaces, homoglyphs)', () => {
    const r = reg();
    const tst = tokenById(r, TOKEN.covenantId)!;
    const other = 'cd'.repeat(32);
    expect(lookalikeReport(r, 'XYZ', other, 'not related').level).toBe('unknown');
    const byTicker = lookalikeReport(r, 'XYZ', other, 't s t');
    expect(byTicker.level).toBe('strong');
    expect(byTicker.lookalikes[0]).toMatchObject({ kind: 'confusable-name' });
    const spaced = tst.name.toUpperCase().split('').join(' ');
    expect(lookalikeReport(r, 'XYZ', other, spaced).level).toBe('strong');
    const cyr = tst.name.replace(/[AEOPCXTH]/gi, (c) => ({ A: 'А', E: 'Е', O: 'О', P: 'Р', C: 'С', X: 'Х', T: 'Т', H: 'Н' }[c.toUpperCase() as 'A'] ?? c));
    expect(lookalikeReport(r, 'XYZ', other, cyr).level).toBe('strong');
  });

  it('sanitizeUntrusted neutralises control / bidi characters and caps the length', () => {
    expect(sanitizeUntrusted('A‮B​C')).toBe('A�B�C');
    expect(sanitizeUntrusted('x'.repeat(100), 10)).toBe('x'.repeat(10) + '…');
  });

  it('tokenMarketInput returns the snake_case shape of toTokenMarket', () => {
    const tst = tokenById(reg(), TOKEN.covenantId)!;
    expect(tokenMarketInput(tst)).toEqual({
      ticker: 'TST', covenant_id: TOKEN.covenantId, template_id: 'kcc20-ref-3x3', extension_commitment: TOKEN.ext, decimals: 3, tick: null,
    });
  });
});

describe('on-chain verification', () => {
  const reg = () => parse(tradableRegistryJson());
  const placed = placeGolden(kob, 'create.ask');
  /** the custody token output of the placement tx, as a chain UTXO */
  const custody = (): ChainTokenUtxo => {
    const c = placed.recovered[0].custody!;
    const out = placed.signed.tx.outputs[c.output];
    return { covenantId: out.covenant!.covenantId, scriptPublicKey: out.scriptPublicKey, state: c.state };
  };

  it('accepts a genuine token UTXO', () => {
    const tst = tokenById(reg(), TOKEN.covenantId)!;
    expect(verifyTokenUtxo(kob, tst, custody())).toEqual({ ok: true, problems: [] });
  });

  it('rejects a wrong covenant id, extension commitment or script', () => {
    const tst = tokenById(reg(), TOKEN.covenantId)!;
    const good = custody();
    expect(verifyTokenUtxo(kob, tst, { ...good, covenantId: 'ab'.repeat(32) }).problems.map((p) => p.code)).toEqual(['covenant-id-mismatch']);
    expect(verifyTokenUtxo(kob, tst, { ...good, covenantId: null }).ok).toBe(false);
    const other = { ...good, state: { ...good.state, extension_commitment: 'dd'.repeat(32) } };
    const codes2 = verifyTokenUtxo(kob, tst, other).problems.map((p) => p.code);
    expect(codes2).toContain('extension-mismatch');
    expect(codes2).toContain('script-mismatch');
    // the state amount is part of the script: a UTXO whose script does not match its claimed state is not this token
    const forged = { ...good, state: { ...good.state, amount: '999999' } };
    expect(verifyTokenUtxo(kob, tst, forged).problems.map((p) => p.code)).toEqual(['script-mismatch']);
    // a script of the OTHER program with the same state
    const eight = tokenById(reg(), '80'.repeat(32))!;
    expect(verifyTokenUtxo(kob, { ...eight, covenantId: TOKEN.covenantId }, good).problems.map((p) => p.code)).toEqual(['script-mismatch']);
  });

  it('a token whose registry template disagrees with the pinned program never verifies on chain', () => {
    const doc = tradableRegistryJson();
    for (const t of doc.templates) if (t.id === 'kcc20-ref-3x3') t.template_hash = 'ab'.repeat(32);
    const tst = tokenById(parse(doc), TOKEN.covenantId)!;
    expect(tst.tradable).toBe(false);
    expect(verifyTokenUtxo(kob, tst, custody()).problems.map((p) => p.code)).toEqual(['template-mismatch']);
  });

  it('verifyIndexerToken cross-checks template hash, extension commitment, covenant id and decimals', () => {
    const tst = tokenById(reg(), TOKEN.covenantId)!;
    const view: IndexerTokenView = {
      ticker: 'TST', covenant_id: TOKEN.covenantId, template_hash: TOKEN.templateHash, extension_commitment: TOKEN.ext, scale: 1000, decimals: 3, open_asks: 0, open_bids: 0,
    };
    expect(verifyIndexerToken(tst, view)).toEqual({ ok: true, problems: [], missing: [] });
    // the indexer's standard scale must be 10^decimals of the registry token (another scale quotes another whole token)
    expect(verifyIndexerToken(tst, { ...view, scale: 100 }).problems.map((p) => p.code)).toEqual(['scale-mismatch']);
    expect(verifyIndexerToken(tst, { ...view, scale: null }).ok).toBe(true);
    expect(verifyIndexerToken(tst, { ...view, template_hash: 'ab'.repeat(32) }).problems.map((p) => p.code)).toEqual(['template-hash-mismatch']);
    expect(verifyIndexerToken(tst, { ...view, extension_commitment: 'ab'.repeat(32) }).problems.map((p) => p.code)).toEqual(['extension-mismatch']);
    expect(verifyIndexerToken(tst, { ...view, covenant_id: 'ab'.repeat(32) }).problems.map((p) => p.code)).toEqual(['covenant-id-mismatch']);
    expect(verifyIndexerToken(tst, { ...view, decimals: 8 }).problems.map((p) => p.code)).toEqual(['decimals-mismatch']);
    const partial = verifyIndexerToken(tst, { ...view, template_hash: null, extension_commitment: null });
    expect(partial.ok).toBe(false);
    expect(partial.missing).toEqual(['template_hash', 'extension_commitment']);
    expect(partial.problems).toEqual([]);
  });
});

describe('registry genesis record (C1 / C2) and the official rule', () => {
  const doc = (over: Record<string, unknown>) => {
    const raw = clone(tradableRegistryJson());
    Object.assign(raw.tokens[0], { genesis_verified: true, genesis: clone(GENESIS) }, over);
    return raw;
  };
  it('official needs genesis_verified and a genesis record that found no live mint authority', () => {
    expect(codes(doc({ official: true }))).toEqual([]);
    expect(codes(doc({ official: true, genesis_verified: undefined, genesis: undefined }))).toEqual(['not-listable', 'not-listable']);
    expect(codes(doc({ official: true, genesis: { ...GENESIS, live_minters: undefined } }))).toEqual(['not-listable']);
    const live = { ...GENESIS, live_minters: ['cd'.repeat(32) + ':3'] };
    expect(codes(doc({ official: true, genesis: live, warning: 'active mint authority' }))).toEqual(['not-listable']);
    // a live minter needs a warning; listed without official is fine with it
    expect(codes(doc({ genesis: live }))).toEqual(['wrong-type']);
    expect(codes(doc({ genesis: live, warning: 'active mint authority: the issuer can mint without limit' }))).toEqual([]);
  });
  it('checks the record shape', () => {
    const bad: [string, Record<string, unknown>, RegistryIssueCode][] = [
      ['record without genesis_verified', { genesis_verified: undefined }, 'wrong-type'],
      ['genesis_verified without verified', { verified: false, status: 'pending-review' }, 'not-listable'],
      ['bad txid', { genesis: { ...GENESIS, txid: 'AB'.repeat(32) } }, 'bad-hex'],
      ['unordered outputs', { genesis: { ...GENESIS, outputs: [2, 1] } }, 'wrong-type'],
      ['no outputs', { genesis: { ...GENESIS, outputs: [] } }, 'wrong-type'],
      ['minter not a genesis output', { genesis: { ...GENESIS, minter_outputs: [7] } }, 'wrong-type'],
      ['negative supply', { genesis: { ...GENESIS, supply: -1 } }, 'wrong-type'],
      ['empty source', { genesis: { ...GENESIS, source: '' } }, 'wrong-type'],
      ['bad outpoint', { genesis: { ...GENESIS, live_minters: ['xyz:1'] }, warning: 'w' }, 'wrong-type'],
      ['unknown field', { genesis: { ...GENESIS, extra: 1 } }, 'unknown-field'],
    ];
    for (const [name, over, code] of bad) expect(codes(doc(over)), name).toContain(code);
  });
});

describe('registry fields of the contracts review: genesis_verified and warning (optional)', () => {
  const base = (over: Record<string, unknown>) => {
    const raw = JSON.parse(JSON.stringify(tradableRegistryJson()));
    Object.assign(raw.tokens[0], over);
    return raw;
  };
  it('are optional: a registry without them parses and reports genesisVerified null (= not verified)', () => {
    const reg = parseRegistry(tradableRegistryJson(), { kob });
    expect(reg.tokens[0].genesisVerified).toBeNull();
    expect(reg.tokens[0].warning).toBeNull();
  });
  it('are carried into TokenInfo when present and type-checked', () => {
    const reg = parseRegistry(base({ genesis_verified: true, warning: 'genesis checked by hand' }), { kob });
    expect(reg.tokens[0]).toMatchObject({ genesisVerified: true, warning: 'genesis checked by hand' });
    expect(() => parseRegistry(base({ genesis_verified: 'yes' }), { kob })).toThrow(/genesis_verified must be a boolean/);
    expect(() => parseRegistry(base({ warning: '' }), { kob })).toThrow(/warning must be/);
    expect(() => parseRegistry(base({ warning: 'a\u202eb' }), { kob })).toThrow(/warning must be/);
  });
});
